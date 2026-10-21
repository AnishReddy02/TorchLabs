#!/bin/sh
set -eu
mkdir -p logs results
echo "battery=96 localization=healthy motors=ready" | tee logs/robot.log
echo "diagnostic channel initialized" >&2
if [ "${FAKE_ROBOT_RESULT:-PASS}" = "FAIL" ]; then echo '{"passed":false}' > results/result.json; exit 42; fi
echo '{"passed":true,"checks":3}' > results/result.json
echo "PASS: fake robot health checks"
