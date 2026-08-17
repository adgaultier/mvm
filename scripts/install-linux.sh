#!/usr/bin/env bash
set -e

. /etc/os-release

case "$ID" in
    arch)
        sudo pacman -Syu --needed \
            rust \
            libkrun \
            libkrunfw
        ;;

    fedora)
        sudo dnf install -y \
            rust \
            libkrun \
            libkrun-devel \
            libkrunfw \
            libkrunfw-devel
        ;;

    ubuntu)
        sudo apt update
        sudo apt install -y \
            rustc \
            cargo \
            libkrunfw5 \
            libkrunfw-dev
        ;;

    *)
        echo "Unsupported OS: $ID"
        exit 1
        ;;
esac
