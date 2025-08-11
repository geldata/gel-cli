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
    ["", "--nightly"]
)]
fn package(distro: Distro, version: &str) -> anyhow::Result<()> {
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
            edgedb server install {version}
            edgedb instance create test1 {version}
            val=$(edgedb -Itest1 --wait-until-available=60s \
                query "SELECT 1+1")
            test "$val" = "2"
            edgedb instance logs -I test1
            timeout 180 edgedb instance destroy -I test1 --non-interactive
            edgedb server uninstall --all --verbose
        "###,
        ),
    )
    .success();
    Ok(())
}
