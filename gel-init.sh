#!/bin/sh
# shellcheck shell=dash

# Portions Copyright (c) 2020 MagicStack Inc.
# Portions Copyright (c) 2016 The Rust Project Developers.
#
# This is a simple script that can be downloaded from https://geldata.com to
# install `gel` CLI tools. Its job is to detect the host platform and to
# download and run the relevant installer.

set -u

EDGEDB_PKG_ROOT="${EDGEDB_PKG_ROOT:-https://packages.geldata.com}"

usage() {
    cat 1>&2 <<EOF
gel-init
The installer for gel command-line tools

USAGE:
    gel-init [FLAGS] [OPTIONS]

FLAGS:
    -h, --help              Print help information
        --nightly           Install nightly version of command-line tools
        --testing           Install testing version of command-line tools
    -y                      Disable confirmation prompt
        --no-modify-path    Do not configure the PATH environment variable
    -q, --quiet             Disable progress output
    -v, --verbose           Enable verbose output
EOF
}

main() {
    downloader --check
    need_cmd uname
    need_cmd mktemp
    need_cmd chmod
    need_cmd mkdir
    need_cmd rm
    need_cmd rmdir

    get_architecture || return 1
    local _arch="$RETVAL"
    assert_nz "$_arch" "arch"

    # initial check of arguments
    local need_tty=yes
    local suffix=""
    for arg in "$@"; do
        case "$arg" in
            --nightly)
                suffix="nightly"
                ;;
            --testing)
                suffix="testing"
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            -y)
                # user wants to skip the prompt -- we don't need /dev/tty
                need_tty=no
                ;;
            *)
                ;;
        esac
    done

    local _ext=""
    case "$_arch" in
        *windows*)
            _ext=".exe"
            ;;
    esac
    local _url="${EDGEDB_PKG_ROOT}/dist/${_arch}${suffix:+.}${suffix}/gel-cli${_ext}"

    local _dir
    _dir="$(mktemp -d 2>/dev/null || ensure mktemp -d -t gel)"
    local _file="${_dir}/gel${_ext}"

    local _ansi_escapes_are_valid=false
    if [ -t 2 ]; then
        if [ "${TERM+set}" = 'set' ]; then
            case "$TERM" in
                xterm*|rxvt*|urxvt*|linux*|vt*)
                    _ansi_escapes_are_valid=true
                ;;
            esac
        fi
    fi


    if $_ansi_escapes_are_valid; then
        printf "\33[1minfo:\33[0m downloading installer\n" 1>&2
    else
        printf '%s\n' 'info: downloading installer' 1>&2
    fi

    ensure mkdir -p "$_dir"
    ensure downloader "$_url" "$_file" "$_arch"
    ensure chmod u+x "$_file"
    if [ ! -x "$_file" ]; then
        printf '%s\n' "Cannot execute $_file (likely because of mounting /tmp as noexec)." 1>&2
        printf '%s\n' "Please copy the file to a location where you can execute binaries and run ./gel-cli${_ext} _self_install." 1>&2
        exit 1
    fi

    if [ "$need_tty" = "yes" ]; then
        # The installer is going to want to ask for confirmation by
        # reading stdin.  This script was piped into `sh` though and
        # doesn't have stdin to pass to its children. Instead we're going
        # to explicitly connect /dev/tty to the installer's stdin.
        if [ ! -t 1 ]; then
            err "Unable to run interactively. Run with -y to accept defaults, --help for additional options"
        fi

        ignore "$_file" _self_install "$@" < /dev/tty
    else
        ignore "$_file" _self_install "$@"
    fi

    local _retval=$?

    ignore rm "$_file" 2> /dev/null  # CLI will try to remove self
    ignore rmdir "$_dir"

    return "$_retval"
}

get_architecture() {
    local _ostype
    local _cputype

    _ostype="$(uname -s)"
    _cputype="$(uname -m)"

    if [ "$_ostype" = Darwin ]; then
        # Darwin `uname -m` lies
        if [ "$_cputype" = i386 ] && [ "$(sysctl -in hw.optional.x86_64)" = 1 ]; then
            _cputype=x86_64
        fi
        if [ "$_cputype" = x86_64 ] && [ "$(sysctl -in sysctl.proc_translated)" = 1 ]; then
            _cputype=aarch64
        fi
    fi

    case "$_ostype" in

        Linux)
            _ostype=unknown-linux-musl
            ;;

        Darwin)
            _ostype=apple-darwin
            ;;

        MINGW* | MSYS* | CYGWIN*)
            _ostype=pc-windows-msvc
            ;;

        *)
            err "unsupported OS: $_ostype"
            ;;

    esac

    case "$_cputype" in

        x86_64 | x86-64 | x64 | amd64)
            _cputype=x86_64
            ;;

        arm64 | aarch64)
            _cputype=aarch64
            ;;

        *)
            err "unsupported CPU architecture: $_cputype"
            ;;

    esac

    if [ "$_cputype" = "aarch64" ]; then
        if [ "$_ostype" != "apple-darwin" ] && [ "$_ostype" != "unknown-linux-musl" ]; then
            err "unsupported CPU architecture: $_cputype"
        fi
    fi

    local _arch="${_cputype}-${_ostype}"

    RETVAL="$_arch"
}

say() {
    printf 'gel-init: %s\n' "$1"
}

err() {
    say "$1" >&2
    exit 1
}

need_cmd() {
    if ! check_cmd "$1"; then
        err "need '$1' (command not found)"
    fi
}

check_cmd() {
    command -v "$1" > /dev/null 2>&1
}

assert_nz() {
    if [ -z "$1" ]; then err "assert_nz $2"; fi
}

# Run a command that should never fail. If the command fails execution
# will immediately terminate with an error showing the failing
# command.
ensure() {
    if ! "$@"; then err "command failed: $*"; fi
}

# This is just for indicating that commands' results are being
# intentionally ignored. Usually, because it's being executed
# as part of error handling.
ignore() {
    "$@"
}

# This wraps curl or wget. Try curl first, if not installed,
# use wget instead.
downloader() {
    local _dld
    local _ciphersuites
    local _err
    local _status
    local _retry
    if check_cmd curl; then
        _dld=curl
    elif check_cmd wget; then
        _dld=wget
    else
        _dld='curl or wget' # to be used in error message of need_cmd
    fi

    if [ "$1" = --check ]; then
        need_cmd "$_dld"
    elif [ "$_dld" = curl ]; then
        check_curl_for_retry_support
        _retry="$RETVAL"
        get_ciphersuites_for_curl
        _ciphersuites="$RETVAL"
        if [ -n "$_ciphersuites" ]; then
            _err=$(curl $_retry --proto '=https' --tlsv1.2 --ciphers "$_ciphersuites" --silent --show-error --fail --location "$1" --output "$2" 2>&1)
            _status=$?
        else
            echo "Warning: Not enforcing strong cipher suites for TLS, this is potentially less secure"
            if ! check_help_for "$3" curl --proto --tlsv1.2; then
                echo "Warning: Not enforcing TLS v1.2, this is potentially less secure"
                _err=$(curl $_retry --silent --show-error --fail --location "$1" --output "$2" 2>&1)
                _status=$?
            else
                _err=$(curl $_retry --proto '=https' --tlsv1.2 --silent --show-error --fail --location "$1" --output "$2" 2>&1)
                _status=$?
            fi
        fi
        if [ -n "$_err" ]; then
            echo "$_err" >&2
            if echo "$_err" | grep -q 404$; then
                err "installer for platform '$3' not found, this may be unsupported"
            fi
        fi
        return $_status
    elif [ "$_dld" = wget ]; then
        if [ "$(wget -V 2>&1|head -2|tail -1|cut -f1 -d" ")" = "BusyBox" ]; then
            echo "Warning: using the BusyBox version of wget.  Not enforcing strong cipher suites for TLS or TLS v1.2, this is potentially less secure"
            _err=$(wget "$1" -O "$2" 2>&1)
            _status=$?
        else
            get_ciphersuites_for_wget
            _ciphersuites="$RETVAL"
            if [ -n "$_ciphersuites" ]; then
                _err=$(wget --https-only --secure-protocol=TLSv1_2 --ciphers "$_ciphersuites" "$1" -O "$2" 2>&1)
                _status=$?
            else
                echo "Warning: Not enforcing strong cipher suites for TLS, this is potentially less secure"
                if ! check_help_for "$3" wget --https-only --secure-protocol; then
                    echo "Warning: Not enforcing TLS v1.2, this is potentially less secure"
                    _err=$(wget "$1" -O "$2" 2>&1)
                    _status=$?
                else
                    _err=$(wget --https-only --secure-protocol=TLSv1_2 "$1" -O "$2" 2>&1)
                    _status=$?
                fi
            fi
        fi
        if [ -n "$_err" ]; then
            echo "$_err" >&2
            if echo "$_err" | grep -q ' 404 Not Found$'; then
                err "installer for platform '$3' not found, this may be unsupported"
            fi
        fi
        return $_status
    else
        err "Unknown downloader"   # should not reach here
    fi
}

check_help_for() {
    local _arch
    local _cmd
    local _arg
    _arch="$1"
    shift
    _cmd="$1"
    shift

    local _category
    if "$_cmd" --help | grep -q 'For all options use the manual or "--help all".'; then
      _category="all"
    else
      _category=""
    fi

    case "$_arch" in

        *darwin*)
        if check_cmd sw_vers; then
            case $(sw_vers -productVersion) in
                10.*)
                    # If we're running on macOS, older than 10.13, then we always
                    # fail to find these options to force fallback
                    if [ "$(sw_vers -productVersion | cut -d. -f2)" -lt 13 ]; then
                        # Older than 10.13
                        echo "Warning: Detected macOS platform older than 10.13"
                        return 1
                    fi
                    ;;
                11.*)
                    # We assume Big Sur will be OK for now
                    ;;
                *)
                    # Unknown product version, warn and continue
                    echo "Warning: Detected unknown macOS major version: $(sw_vers -productVersion)"
                    echo "Warning TLS capabilities detection may fail"
                    ;;
            esac
        fi
        ;;

    esac

    for _arg in "$@"; do
        if ! "$_cmd" --help "$_category" | grep -q -- "$_arg"; then
            return 1
        fi
    done

    true # not strictly needed
}

# Check if curl supports the --retry flag, then pass it to the curl invocation.
check_curl_for_retry_support() {
  local _retry_supported=""
  # "unspecified" is for arch, allows for possibility old OS using macports, homebrew, etc.
  if check_help_for "notspecified" "curl" "--retry"; then
    _retry_supported="--retry 3"
  fi

  RETVAL="$_retry_supported"

}

# Return cipher suite string specified by user, otherwise return strong TLS 1.2-1.3 cipher suites
# if support by local tools is detected. Detection currently supports these curl backends:
# GnuTLS and OpenSSL (possibly also LibreSSL and BoringSSL). Return value can be empty.
get_ciphersuites_for_curl() {
    if [ -n "${RUSTUP_TLS_CIPHERSUITES-}" ]; then
        # user specified custom cipher suites, assume they know what they're doing
        RETVAL="$RUSTUP_TLS_CIPHERSUITES"
        return
    fi

    local _openssl_syntax="no"
    local _gnutls_syntax="no"
    local _backend_supported="yes"
    if curl -V | grep -q ' OpenSSL/'; then
        _openssl_syntax="yes"
    elif curl -V | grep -iq ' LibreSSL/'; then
        _openssl_syntax="yes"
    elif curl -V | grep -iq ' BoringSSL/'; then
        _openssl_syntax="yes"
    elif curl -V | grep -iq ' GnuTLS/'; then
        _gnutls_syntax="yes"
    else
        _backend_supported="no"
    fi

    local _args_supported="no"
    if [ "$_backend_supported" = "yes" ]; then
        # "unspecified" is for arch, allows for possibility old OS using macports, homebrew, etc.
        if check_help_for "notspecified" "curl" "--tlsv1.2" "--ciphers" "--proto"; then
            _args_supported="yes"
        fi
    fi

    local _cs=""
    if [ "$_args_supported" = "yes" ]; then
        if [ "$_openssl_syntax" = "yes" ]; then
            _cs=$(get_strong_ciphersuites_for "openssl")
        elif [ "$_gnutls_syntax" = "yes" ]; then
            _cs=$(get_strong_ciphersuites_for "gnutls")
        fi
    fi

    RETVAL="$_cs"
}

# Return cipher suite string specified by user, otherwise return strong TLS 1.2-1.3 cipher suites
# if support by local tools is detected. Detection currently supports these wget backends:
# GnuTLS and OpenSSL (possibly also LibreSSL and BoringSSL). Return value can be empty.
get_ciphersuites_for_wget() {
    if [ -n "${RUSTUP_TLS_CIPHERSUITES-}" ]; then
        # user specified custom cipher suites, assume they know what they're doing
        RETVAL="$RUSTUP_TLS_CIPHERSUITES"
        return
    fi

    local _cs=""
    if wget -V | grep -q '\-DHAVE_LIBSSL'; then
        # "unspecified" is for arch, allows for possibility old OS using macports, homebrew, etc.
        if check_help_for "notspecified" "wget" "TLSv1_2" "--ciphers" "--https-only" "--secure-protocol"; then
            _cs=$(get_strong_ciphersuites_for "openssl")
        fi
    elif wget -V | grep -q '\-DHAVE_LIBGNUTLS'; then
        # "unspecified" is for arch, allows for possibility old OS using macports, homebrew, etc.
        if check_help_for "notspecified" "wget" "TLSv1_2" "--ciphers" "--https-only" "--secure-protocol"; then
            _cs=$(get_strong_ciphersuites_for "gnutls")
        fi
    fi

    RETVAL="$_cs"
}

# Return strong TLS 1.2-1.3 cipher suites in OpenSSL or GnuTLS syntax. TLS 1.2
# excludes non-ECDHE and non-AEAD cipher suites. DHE is excluded due to bad
# DH params often found on servers (see RFC 7919). Sequence matches or is
# similar to Firefox 68 ESR with weak cipher suites disabled via about:config.
# $1 must be openssl or gnutls.
get_strong_ciphersuites_for() {
    if [ "$1" = "openssl" ]; then
        # OpenSSL is forgiving of unknown values, no problems with TLS 1.3 values on versions that don't support it yet.
        echo "TLS_AES_128_GCM_SHA256:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_256_GCM_SHA384:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384"
    elif [ "$1" = "gnutls" ]; then
        # GnuTLS isn't forgiving of unknown values, so this may require a GnuTLS version that supports TLS 1.3 even if wget doesn't.
        # Begin with SECURE128 (and higher) then remove/add to build cipher suites. Produces same 9 cipher suites as OpenSSL but in slightly different order.
        echo "SECURE128:-VERS-SSL3.0:-VERS-TLS1.0:-VERS-TLS1.1:-VERS-DTLS-ALL:-CIPHER-ALL:-MAC-ALL:-KX-ALL:+AEAD:+ECDHE-ECDSA:+ECDHE-RSA:+AES-128-GCM:+CHACHA20-POLY1305:+AES-256-GCM"
    fi
}

main "$@" || exit 1
