#!/bin/sh
set -eu
. ./config.env
mkdir -p logs results
echo "localization: validating frame configuration"
printf 'configured_map_frame=%s\nexpected_map_frame=%s\n' "$MAP_FRAME" "$EXPECTED_MAP_FRAME" | tee logs/configuration.log
if [ "$MAP_FRAME" != "$EXPECTED_MAP_FRAME" ]; then
  printf '{"check":"localization-frame","expected":"%s","actual":"%s","passed":false}\n' "$EXPECTED_MAP_FRAME" "$MAP_FRAME" > results/localization.json
  echo "ERROR localization initialization refused: map frame '$MAP_FRAME' does not match required '$EXPECTED_MAP_FRAME'" >&2
  exit 42
fi
echo '{"check":"localization-frame","passed":true}' > results/localization.json
echo "PASS localization frame check"
