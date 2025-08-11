use test_case::test_matrix;

use crate::common::Distro;
use crate::docker::run_systemd;
use crate::docker::{Context, build_image};
use crate::measure::Time;

#[test_matrix(
    [
        Distro::Ubuntu("noble"),
        Distro::Ubuntu("jammy"),
        Distro::Ubuntu("focal"),
        Distro::Debian("bookworm"),
        Distro::Debian("bullseye"),
    ],
    ["", "--server-version=nightly"]
)]
fn simple_package(distro: Distro, version: &str) -> anyhow::Result<()> {
    let tag_name = distro.tag_name();

    let _tm = Time::measure();
    let context = Context::new()
        .add_file("Dockerfile", distro.dockerfile())?
        .add_sudoers()?
        .add_bin()?;
    build_image(context, &tag_name)?;
    run_systemd(
        &tag_name,
        &format!(
            r###"
            mkdir -p /tmp/test1
            cd /tmp/test1
            edgedb project init --non-interactive {version}
            val=$(edgedb --wait-until-available=60s query "SELECT 7+8")
            test "$val" = "15"
            timeout 120 edgedb project unlink \
                --destroy-server-instance --non-interactive
        "###,
        ),
    )
    .success();
    Ok(())
}
