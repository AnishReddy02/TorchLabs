#!/bin/sh
set -eu
printf '{"mode":"%s","configured":true}\n' "${MODE:-test}" > telemetry.json
echo "configured fake robot in ${MODE:-test} mode"
