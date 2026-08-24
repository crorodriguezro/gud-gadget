# T04 verdict

**BLOCKED — do not promote the selected transport.**

The direct Mir RGB565 + LZ4 transport remained safe in the captured interval:
13,920 accepted/completed transactions and zero poisoned, timed-out,
processing-failed, or ambiguous-accepted transactions were observed. However,
the required physical correctness gate failed when the operator reported an
undersized desktop. The run was stopped before 30 minutes, so resource plateau,
long-duration fence/FD stability, and sustained visual correctness remain
unqualified.

The immediate test configuration issue is that the connected monitor prefers
1920x1080 while T04 forced the Pi's physical output to 1280x720. Selecting
1920x1080 with the existing 1280x720 source invoked scaling and cost about
203 ms even for one-row updates, so that short check is not a passing fix.

Required next gate: choose and document a full-screen source/output geometry
that does not introduce this scaling regression, then repeat T04 from a fresh
evidence directory.
