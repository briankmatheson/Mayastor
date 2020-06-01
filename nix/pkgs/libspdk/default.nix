{ binutils
, callPackage
, cunit
, enableDebug ? true
, fetchFromGitHub
, git
, lcov
, libaio
, libiscsi
, liburing
, libuuid
, nasm
, ncurses
, numactl
, openssl
, python3
, rdma-core
, stdenv
}:
with stdenv.lib;
stdenv.mkDerivation rec {
  src = fetchFromGitHub {
    owner = "openebs";
    repo = "spdk";
    rev = "0dd3398ace940bb291e18138a2535cf6b1e161b5";
    sha256 = "06pch25iq5xdr934njc6f972b4xzdrdm7d1ss15qlj60g0cnzjrg";
    fetchSubmodules = true;
  };

  name = "libspdk";

  buildInputs = [
    binutils
    libaio
    libiscsi.dev
    libuuid
    liburing
    nasm
    ncurses
    numactl
    openssl
    python3
  ] ++ stdenv.lib.optionals enableDebug [ cunit lcov ];

  # add this once we merged this new option from upstream. The tests are going
  # to be split up in unit-test and test configure options. test will contain
  # utilities like bdevperf that is getting more features into it.

  configureFlags = [
    "${enableFeature enableDebug "debug"}"
    "${enableFeature enableDebug "unit-tests"}"
    "${enableFeature enableDebug "tests"}"
    "--without-isal"
    "--with-iscsi-initiator"
    "--with-crypto"
    "--with-uring"
    "--with-internal-vhost-lib"
  ];

  enableParallelBuilding = true;

  preConfigure = ''
    patchShebangs ./.
    substituteInPlace dpdk/config/defconfig_x86_64-native-linux-gcc --replace native default
    # A workaround for https://bugs.dpdk.org/show_bug.cgi?id=356
    substituteInPlace dpdk/lib/Makefile --replace 'DEPDIRS-librte_vhost :=' 'DEPDIRS-librte_vhost := librte_hash'

    # we make use of private header files to generate bindings. To ensure we can find
    # the proper header files we place the header files one level deeper.
    #
    # Normal: /include/spdk/bdev.h
    # Patched: /include/spdk/spdk/bdev.h
    #
    # In this example bdev.h is public.
    # The private header files are stored within /include/spdk.
    # Such that header files that include "spdk/bdev.h" remain working.
    #
    substituteInPlace mk/spdk.common.mk --replace 'includedir?=$(CONFIG_PREFIX)/include' 'includedir?=$(CONFIG_PREFIX)/include/spdk'

  '';

  NIX_CFLAGS_COMPILE = "-mno-movbe -mno-lzcnt -mno-bmi -mno-bmi2 -march=nehalem";
  hardeningDisable = [ "all" ];

  postBuild = ''
    find . -type f -name 'libspdk_ut_mock.a' -delete
    find . -type f -name 'librte_vhost.a' -delete

    $CC -shared -o libspdk.so \
    -lc  -laio -liscsi -lnuma -ldl -lrt -luuid -lpthread -lcrypto -luring \
    -Wl,--whole-archive \
    $(find build/lib -type f -name 'libspdk_*.a*' -o -name 'librte_*.a*') \
    $(find dpdk/build/lib -type f -name 'librte_*.a*') \
    $(find intel-ipsec-mb -type f -name 'libIPSec_*.a*') \
    -Wl,--no-whole-archive

  '';

  postInstall = ''

    pushd include
    find . -type f -name "*.h" -exec install -D "{}" $out/include/spdk/{} \;
    popd

    pushd lib
    find . -type f -name "*.h" -exec install -D "{}" $out/include/spdk/{} \;
    popd

    # copy private headers from bdev modules needed for creating of bdevs
    pushd module
    find . -type f -name "*.h" -exec install -D "{}" $out/include/spdk/{} \;
    popd

    # copy over the library
    cp libspdk.so $out/lib

    if [ $enableDebug ]
    then
      echo "copying files"
      cp -ar test $out/test
    fi

    echo $(find $out -type f -name '*.a*' -delete)
    find . -executable -type f -name 'bdevperf' -exec install -D "{}" $out/bin \;
  '';

  separateDebugInfo = !enableDebug;
  dontStrip = true;

}
