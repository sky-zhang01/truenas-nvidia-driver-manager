#!/usr/bin/env bash
# Shared image job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

args=()
if [ "${IMAGE_NO_CACHE:-${NO_CACHE:-false}}" = true ]; then args+=(--no-cache); fi
docker build "${args[@]}" --iidfile nvm-image.id .
image_id="$(cat nvm-image.id)"
docker image inspect "${image_id}" >/dev/null
docker save "${image_id}" -o nvm-image.tar
echo "image: $(wc -c < nvm-image.tar | tr -d ' ') bytes in the docker save archive (sha256 $(sha256sum nvm-image.tar | awk '{print $1}'))"
python3 ci/image-assert.py nvm-image.tar
bash ci/container/log-object-check-test.sh
bash ci/cleanup-lib-test.sh
NVM_IMAGE="$(cat nvm-image.id)" bash ci/container/test.sh
