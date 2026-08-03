/*
 * tf-pipe-probe - find out who talks on \\.\pipe\logi.trueforce.connect
 *
 * Logitech's TrueForce SDK (trueforce_sdk_x64.dll, the DLL games load) links
 * a small IPC layer, logi::local_connection, with both a client and a server
 * implementation and this pipe name baked in. On Windows the other end is
 * G HUB. Under Proton nothing is there, and the SDK's own strings include
 * "LocalClientImpl::asyncConnection(): failed to connect. Giving up."
 *
 * That matters because the SDK exports logiWheelGetOperatingRangeDegrees and
 * logiWheelGetOperatingRangeBoundsDegrees: a game ASKS for the wheel's
 * rotation. WheelSetOperatingRangeDegrees, by contrast, is not an export at
 * all, only a packet type dispatched to a callback. So the range a game ends
 * up using may well arrive over this pipe, and when nothing answers, what
 * reaches the wheel is 90 degrees, which is exactly the minimum of the
 * wheel's legal 90-2700 range rather than any value a person chose.
 *
 * This probe does not assume which side the game plays. It tries both:
 *
 *   1. Connect as a CLIENT. If that works, the game's SDK is SERVING the
 *      pipe and G HUB would normally be the client.
 *   2. Otherwise create the pipe as a SERVER and wait. If the SDK connects,
 *      the SDK is the client and G HUB normally serves.
 *
 * Either way it hex-dumps whatever crosses the pipe, which is the protocol we
 * would need in order to answer the query ourselves.
 *
 * It never writes to the wheel and never moves anything. It only listens.
 *
 * Build as a PE. Wine 11 no longer runs Winelib ELF binaries, so winegcc
 * without a mingw cross-compiler produces something that will not start:
 *
 *   x86_64-w64-mingw32-gcc -O2 -o tf-pipe-probe.exe tools/tf-pipe-probe.c
 *
 * Run it BEFORE launching the game, with the same WINEPREFIX the game uses.
 * For a Proton title that is
 * ~/.steam/steam/steamapps/compatdata/<appid>/pfx.
 *
 * STATUS: built and smoke-tested. It serves the pipe and waits as intended.
 * What it has NOT yet captured is a real conversation, because the SDK only
 * starts its connection client once a game initialises it; merely loading
 * the DLL is not enough, as tested. So the useful run is on a machine with
 * the affected game: start this first, then launch the game.
 *
 * The prior question, whether the SDK reaches for this pipe under Proton at
 * all, is now answered and the answer is yes. From a reporter's Wine log
 * (issue #27), repeating continuously:
 *
 *   CreateFileW L"\\\\.\\pipe\\logi.trueforce.connect" ... creation 3
 *   CreateFileW Unable to create file ... (status c0000034)
 *
 * creation 3 is OPEN_EXISTING and c0000034 is STATUS_OBJECT_NAME_NOT_FOUND,
 * so the SDK is the client, nothing serves the pipe under Proton, and it
 * retries forever, which matches the library's own
 * "Client failed to connect: polling again in %ld ms".
 */

#include <windows.h>
#include <stdarg.h>
#include <stdio.h>

#define PIPE_NAME "\\\\.\\pipe\\logi.trueforce.connect"
/* The SDK gives up after a while; outlive a game's whole startup. */
#define WAIT_SECONDS 180

static FILE *logfp;

static void say(const char *fmt, ...)
{
	va_list ap;
	SYSTEMTIME t;

	GetLocalTime(&t);
	va_start(ap, fmt);
	printf("[%02d:%02d:%02d.%03d] ", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
	vprintf(fmt, ap);
	printf("\n");
	fflush(stdout);
	va_end(ap);

	if (logfp) {
		va_start(ap, fmt);
		fprintf(logfp, "[%02d:%02d:%02d.%03d] ", t.wHour, t.wMinute,
			t.wSecond, t.wMilliseconds);
		vfprintf(logfp, fmt, ap);
		fprintf(logfp, "\n");
		fflush(logfp);
		va_end(ap);
	}
}

/* Dump bytes both readably and as hex; the protocol is unknown, so show all. */
static void dump(const char *tag, const unsigned char *buf, DWORD n)
{
	char hex[3 * 16 + 1], txt[17];
	DWORD i, j;

	say("%s: %lu byte(s)", tag, (unsigned long)n);
	for (i = 0; i < n; i += 16) {
		hex[0] = txt[0] = 0;
		for (j = 0; j < 16 && i + j < n; j++) {
			sprintf(hex + 3 * j, "%02x ", buf[i + j]);
			txt[j] = (buf[i + j] >= 32 && buf[i + j] < 127)
				 ? (char)buf[i + j] : '.';
		}
		txt[j] = 0;
		say("  %04lx  %-48s |%s|", (unsigned long)i, hex, txt);
	}
}

/* Read until the peer goes away, dumping everything. */
static void pump(HANDLE h, const char *role)
{
	unsigned char buf[4096];
	DWORD n;

	say("%s: connected, reading (Ctrl-C to stop)", role);
	for (;;) {
		if (!ReadFile(h, buf, sizeof(buf), &n, NULL)) {
			say("%s: read ended (error %lu)", role,
			    (unsigned long)GetLastError());
			return;
		}
		if (n == 0)
			continue;
		dump(role, buf, n);
	}
}

int main(void)
{
	HANDLE h;
	int i;

	/*
	 * Log to a fixed, findable place rather than the working directory.
	 * A Wine app's idea of "here" is not the shell's, and a game launcher
	 * sets its own; a tester should not have to hunt. C:\ inside the
	 * prefix is drive_c/ on the Linux side.
	 */
	logfp = fopen("C:\\tf-pipe-probe.log", "w");
	if (!logfp)
		logfp = fopen("tf-pipe-probe.log", "w");

	say("probing %s", PIPE_NAME);
	say("this only listens; it never writes to the wheel");

	/*
	 * Step 1: is something already serving? If the game's SDK creates the
	 * pipe, we can walk up to it as a client.
	 */
	h = CreateFileA(PIPE_NAME, GENERIC_READ | GENERIC_WRITE, 0, NULL,
			OPEN_EXISTING, 0, NULL);
	if (h != INVALID_HANDLE_VALUE) {
		say("RESULT: something is ALREADY SERVING this pipe.");
		say("  So the SDK (or another Logitech component) is the server");
		say("  and G HUB would normally be the client.");
		pump(h, "server->us");
		CloseHandle(h);
		goto done;
	}
	say("nothing is serving it yet (error %lu), so we will serve it",
	    (unsigned long)GetLastError());

	/*
	 * Step 2: serve it ourselves and see whether the SDK comes to us. This
	 * is the case that would confirm the hypothesis: the SDK is a client
	 * looking for a G HUB that does not exist under Proton.
	 */
	h = CreateNamedPipeA(PIPE_NAME,
			     PIPE_ACCESS_DUPLEX,
			     PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
			     PIPE_UNLIMITED_INSTANCES,
			     4096, 4096, 0, NULL);
	if (h == INVALID_HANDLE_VALUE) {
		say("FAILED to create the pipe (error %lu). Cannot probe.",
		    (unsigned long)GetLastError());
		goto done;
	}

	say("serving. NOW START THE GAME. Waiting up to %d seconds...", WAIT_SECONDS);
	for (i = 0; i < WAIT_SECONDS; i++) {
		if (ConnectNamedPipe(h, NULL) ||
		    GetLastError() == ERROR_PIPE_CONNECTED) {
			say("RESULT: the SDK CONNECTED to us.");
			say("  Confirms it is a client looking for a peer that");
			say("  does not exist under Proton. Whatever follows is");
			say("  the protocol we would have to answer.");
			pump(h, "sdk->us");
			DisconnectNamedPipe(h);
			CloseHandle(h);
			goto done;
		}
		if (GetLastError() != ERROR_PIPE_LISTENING &&
		    GetLastError() != ERROR_IO_PENDING) {
			say("connect wait failed (error %lu)",
			    (unsigned long)GetLastError());
			break;
		}
		Sleep(1000);
	}
	say("RESULT: nothing connected in %d seconds.", WAIT_SECONDS);
	say("  Either the SDK never tries this pipe under Proton, or it gave");
	say("  up before the game started. Start the probe first, then the game.");
	CloseHandle(h);

done:
	say("done; transcript in tf-pipe-probe.log");
	if (logfp)
		fclose(logfp);
	return 0;
}
