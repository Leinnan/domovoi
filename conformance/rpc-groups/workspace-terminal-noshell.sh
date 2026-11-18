# A shell that cannot be started: it is there, and not executable.
printf '#!/bin/sh\n' > bin/noshell
chmod 644 bin/noshell
mkdir -p work
