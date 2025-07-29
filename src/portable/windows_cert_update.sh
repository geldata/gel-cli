#!/bin/bash
# Note that this script is run inside of WSL!
set -eux

SCRIPT=${BASH_SOURCE[0]}
TEMP_DIR=/tmp/cert-update

trap 'rm -rf $TEMP_DIR && rm -f $SCRIPT' EXIT

mkdir -p $TEMP_DIR/{lists,archives}/partial

echo 'deb http://deb.debian.org/debian stable main' > $TEMP_DIR/sources.list
echo 'deb http://deb.debian.org/debian stable-updates main' >> $TEMP_DIR/sources.list

export DEBIAN_FRONTEND=noninteractive

apt-get update -qq \
    -o Dir::Etc::SourceList=$TEMP_DIR/sources.list \
    -o Dir::Cache::Archives=$TEMP_DIR/archives \
    -o Dir::State::Lists=$TEMP_DIR/lists

cd $TEMP_DIR && apt-get download ca-certificates \
    -o Dir::Etc::SourceList=$TEMP_DIR/sources.list \
    -o Dir::Cache::Archives=$TEMP_DIR/archives \
    -o Dir::State::Lists=$TEMP_DIR/lists

find $TEMP_DIR

dpkg --unpack $TEMP_DIR/ca-certificates_*.deb

# We need to copy the concatenated certs to a directory that openssl-probe searches
# https://github.com/alexcrichton/openssl-probe/blob/main/src/lib.rs
CERT_DIR=/etc/ssl
mkdir -p $CERT_DIR/certs
find /usr/share/ca-certificates -name "*.crt" -exec cp {} $CERT_DIR/certs \;
find /usr/share/ca-certificates -name "*.crt" -print0 | xargs -0 cat > $CERT_DIR/ca-certificates.crt

echo "Copied $(ls -l $CERT_DIR | wc -l) certs to $CERT_DIR/certs"
