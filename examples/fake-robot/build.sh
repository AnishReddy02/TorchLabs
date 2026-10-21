#!/bin/sh
set -eu
mkdir -p build logs results
printf 'fake-robot build %s\n' "${GIT_SHA:-local}" > build/robot.bin
echo "built fake autonomy stack"
