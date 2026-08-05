// SPDX-License-Identifier: LGPL-2.1-or-later
/*
 * angle - print the wheel's angle and angular velocity every 100 ms
 * for N seconds (default 10). Turn the wheel during the run.
 */

#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include <trueforce.h>

/* Small readers: the SDK reports through an out parameter and returns a
 * status, so a test that wants the value still has to check the status. */
/* Read a degrees-valued getter, or 0 if it fails; a sampling loop wants a
 * number, and the status is checked once before the loop starts. */
static double read_deg(int (*fn)(int, double *), int index)
{
	double v = 0.0;

	return fn(index, &v) == LOGITF_OK ? v : 0.0;
}

static bool tf_available(void)
{
	bool a = false;

	return logiTrueForceAvailable(&a) == LOGITF_OK && a;
}

int main(int argc, char **argv)
{
	int secs = argc > 1 ? atoi(argv[1]) : 10;
	int index = argc > 2 ? atoi(argv[2]) : 0;

	if (dllOpen() != LOGITF_OK) {
		fprintf(stderr, "dllOpen failed\n");
		return 1;
	}
	if (!tf_available()) {
		fprintf(stderr, "no wheel at index %d\n", index);
		return 1;
	}

	/* First call starts the status thread. */
	if (logiTrueForceGetAngleDegrees(index, &(double){0}) != LOGITF_OK) {
		fprintf(stderr, "cannot read the wheel angle at index %d\n", index);
		return 1;
	}

	printf("turn the wheel; sampling every 100 ms for %d s...\n", secs);
	for (int i = 0; i < secs * 10; i++) {
		double a  = read_deg(logiTrueForceGetAngleDegrees, index);
		double v  = read_deg(logiTrueForceGetAngularVelocityDegrees, index);

		printf("\rangle = %+8.2f deg   velocity = %+8.1f deg/s   ", a, v);
		fflush(stdout);
		usleep(100000);
	}
	putchar('\n');

	dllClose();
	return 0;
}
