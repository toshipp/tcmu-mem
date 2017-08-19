#!/bin/sh
set -eu

CURRENT=$(cd $(dirname $0); pwd)
rust-bindgen --no-layout-tests \
             --no-derive-debug \
             ${CURRENT}/tcmu.h
