/*
 * tf-range-proxy - answer the TrueForce SDK's rotation question correctly.
 *
 * The problem, established in issue #27. A sim asks Logitech's TrueForce SDK
 * how far the wheel turns. On Windows the SDK gets that from G HUB over a
 * local pipe. Under Proton nothing serves that pipe, the SDK gives up, and
 * what the game ends up using is 90 degrees: not a number anyone chose, but
 * the minimum of the wheel's legal 90-2700 range. The game then maps full
 * steering lock onto 45 degrees each way. Assetto Corsa Competizione clamps
 * there and will not steer past it.
 *
 * We cannot answer that pipe ourselves: the SDK checks that whoever serves
 * it is code-signed by Logitech, which we are not and will not pretend to
 * be. But we do not need to. The game loads the SDK through a CLSID our own
 * shim installer writes into the prefix, so we choose which library it gets.
 *
 * This is that library. It forwards all 52 other entry points straight to
 * Logitech's real DLL through PE export forwarding, so force feedback and
 * TrueForce behave exactly as before and pay nothing for passing through.
 * It implements only the four rotation getters, answering with the range the
 * wheel is actually set to, read from this driver's sysfs.
 *
 * To be clear about what this is not: it does not patch Logitech's binary,
 * does not bypass their signature check, and does not impersonate anything.
 * It supplies a value the SDK cannot obtain on a system where G HUB does not
 * exist.
 *
 * Install: this DLL goes where the shim installer points the CLSID, with
 * Logitech's own DLL beside it renamed trueforce_real.dll.
 *
 * Build:
 *   x86_64-w64-mingw32-gcc -O2 -shared -o trueforce_sdk_x64.dll \
 *       tools/tf-range-proxy.c tools/tf-range-proxy.def
 */

#include <windows.h>
#include <stdio.h>

#define _USE_MATH_DEFINES
#include <math.h>

/*
 * Where the kernel driver publishes the range. The direct-drive wheels call
 * it wheel_range; the G923 has no wheel_* attributes at all and calls the
 * same setting range. Both are read, in that order.
 */
#define SYSFS_GLOB "Z:\\sys\\class\\hidraw\\*"

static FILE *logfp;
static CRITICAL_SECTION loglock;
static int log_ready;

static void say(const char *fmt, ...)
{
	va_list ap;

	if (!log_ready)
		return;
	EnterCriticalSection(&loglock);
	if (logfp) {
		SYSTEMTIME t;
		GetLocalTime(&t);
		fprintf(logfp, "[%02d:%02d:%02d.%03d] ", t.wHour, t.wMinute,
			t.wSecond, t.wMilliseconds);
		va_start(ap, fmt);
		vfprintf(logfp, fmt, ap);
		va_end(ap);
		fprintf(logfp, "\n");
		fflush(logfp);
	}
	LeaveCriticalSection(&loglock);
}

/* Read one integer out of a sysfs file, or -1. */
static int read_int_file(const char *path)
{
	char buf[64];
	DWORD n = 0;
	HANDLE h;
	int v;

	h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
			NULL, OPEN_EXISTING, 0, NULL);
	if (h == INVALID_HANDLE_VALUE)
		return -1;
	if (!ReadFile(h, buf, sizeof(buf) - 1, &n, NULL) || n == 0) {
		CloseHandle(h);
		return -1;
	}
	CloseHandle(h);
	buf[n] = 0;
	v = atoi(buf);
	return v > 0 ? v : -1;
}

/*
 * Walk the hidraw nodes for the first wheel that publishes a range. There is
 * normally one; a rig with two wheels takes the first, which is the same
 * choice every other tool here makes.
 */
static int wheel_range_degrees(void)
{
	WIN32_FIND_DATAA fd;
	HANDLE h;
	char path[MAX_PATH + 64];
	int v;

	h = FindFirstFileA(SYSFS_GLOB, &fd);
	if (h == INVALID_HANDLE_VALUE)
		return -1;
	do {
		if (fd.cFileName[0] == '.')
			continue;
		snprintf(path, sizeof(path),
			 "Z:\\sys\\class\\hidraw\\%s\\device\\wheel_range",
			 fd.cFileName);
		v = read_int_file(path);
		if (v > 0) {
			FindClose(h);
			return v;
		}
		snprintf(path, sizeof(path),
			 "Z:\\sys\\class\\hidraw\\%s\\device\\range",
			 fd.cFileName);
		v = read_int_file(path);
		if (v > 0) {
			FindClose(h);
			return v;
		}
	} while (FindNextFileA(h, &fd));
	FindClose(h);
	return -1;
}

__declspec(dllexport) double logiWheelGetOperatingRangeDegrees(int index)
{
	int v = wheel_range_degrees();

	/*
	 * Deliberately does NOT ask the real library what it would have said.
	 * That was worth knowing and cost a page fault: its getter dereferences
	 * device state that does not exist until a wheel has been opened, so
	 * calling it cold takes the game down with it. A log line is not worth
	 * a crash.
	 */
	if (v > 0) {
		say("GetOperatingRangeDegrees(%d) -> %d (from sysfs)", index, v);
		return (double)v;
	}
	/*
	 * No wheel of ours present. Answering 0 would be a lie of a different
	 * kind, so report the failure and let the caller's own fallback run.
	 */
	say("GetOperatingRangeDegrees(%d): no range in sysfs", index);
	return 0.0;
}

__declspec(dllexport) double logiWheelGetOperatingRangeRadians(int index)
{
	return logiWheelGetOperatingRangeDegrees(index) * (M_PI / 180.0);
}

__declspec(dllexport) int logiWheelGetOperatingRangeBoundsDegrees(int index,
								 double *lo,
								 double *hi)
{
	/*
	 * The wheel's own limits, not the current setting. A game that asks
	 * for the bounds and is told 90 to 90 has nowhere to put a range.
	 */
	if (lo) *lo = 90.0;
	if (hi) *hi = 2700.0;
	say("GetOperatingRangeBoundsDegrees(%d): answering 90..2700", index);
	return 0;
}

__declspec(dllexport) int logiWheelGetOperatingRangeBoundsRadians(int index,
								 double *lo,
								 double *hi)
{
	double a, b;
	int r = logiWheelGetOperatingRangeBoundsDegrees(index, &a, &b);

	if (lo) *lo = a * (M_PI / 180.0);
	if (hi) *hi = b * (M_PI / 180.0);
	return r;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved)
{
	(void)inst;
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH) {
		char path[MAX_PATH + 32], *slash;

		InitializeCriticalSection(&loglock);
		logfp = fopen("C:\\tf-range-proxy.log", "a");
		log_ready = 1;

		/*
		 * Load Logitech's library by absolute path, now, before anything
		 * asks for one of the fifty-two entry points that forward to it.
		 *
		 * A PE forward names its target by module name, and the loader
		 * resolves that through the ordinary search path. That path does
		 * not include this DLL's own directory, so with the game's
		 * working directory somewhere else every forwarded export failed
		 * with ERROR_PROC_NOT_FOUND while the four implemented here kept
		 * working. From the driver's seat that is the worst possible
		 * shape of failure: the rotation is fixed and the wheel goes
		 * completely dead (issue #27).
		 *
		 * Loading it here registers it under its base name, which is the
		 * name the forwards resolve through, so they find it already in
		 * memory rather than going looking.
		 */
		if (GetModuleFileNameA(inst, path, MAX_PATH)) {
			slash = strrchr(path, '\\');
			if (slash) {
				strcpy(slash + 1, "trueforce_real.dll");
				if (LoadLibraryExA(path, NULL,
						   LOAD_WITH_ALTERED_SEARCH_PATH))
					say("loaded Logitech's library from %s", path);
				else
					say("COULD NOT load %s (error %lu): the "
					    "forwarded calls will fail and the "
					    "wheel will feel dead",
					    path, (unsigned long)GetLastError());
			}
		}
		say("wheel range from sysfs = %d", wheel_range_degrees());
	} else if (reason == DLL_PROCESS_DETACH) {
		if (logfp)
			fclose(logfp);
	}
	return TRUE;
}
