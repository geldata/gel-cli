use test_case::test_case;

use crate::common::{dock_debian, dock_ubuntu};
use crate::docker::run_systemd;
use crate::docker::{Context, build_image};
use crate::measure::Time;

#[test_case("test_jammy", &dock_ubuntu("jammy"))]
#[test_case("test_focal", &dock_ubuntu("focal"))]
#[test_case("test_bookworm", &dock_debian("bookworm"))]
#[test_case("test_bullseye", &dock_debian("bullseye"))]
fn package(tagname: &str, dockerfile: &str) -> anyhow::Result<()> {
    let _tm = Time::measure();
    let context = Context::new()
        .add_file("Dockerfile", dockerfile)?
        .add_sudoers()?
        .add_bin()?;
    build_image(context, tagname)?;
    run_systemd(
        tagname,
        r###"
        edgedb server install --version=4.8
        edgedb instance create test1 --version=4.8

        ver1=$(edgedb -Itest1 --wait-until-available=60s query --output-format=tab-separated \
            'SELECT sys::get_version_as_str()')
        [[ $ver1 =~ ^4\.8 ]]

        edgedb --wait-until-available=60s -Itest1 query '
            CREATE TYPE Type1 {
                CREATE PROPERTY prop1 -> str;
            }
        ' 'INSERT Type1 { prop1 := "value1" }'
        if ! edgedb instance upgrade -I test1 --to-version=5 --non-interactive; then
            res=$?
            journalctl -xe
            exit $res
        fi
        ver2=$(edgedb -Itest1 --wait-until-available=60s query --output-format=tab-separated \
            'SELECT sys::get_version_as_str()')
        [[ $ver2 =~ ^5\.[0-9]+\+ ]]

        val=$(edgedb -Itest1 --wait-until-available=60s query --output-format=tab-separated \
            'SELECT Type1 { prop1 }')
        test "$val" = "value1"

        if ! edgedb instance revert -I test1 --no-confirm; then
            res=$?
            journalctl -xe
            exit $res
        fi
        ver2=$(edgedb -Itest1 --wait-until-available=60s query --output-format=tab-separated \
            'SELECT sys::get_version_as_str()')
        [[ $ver1 =~ ^4\.8 ]]

        val=$(edgedb -Itest1 --wait-until-available=60s query --output-format=tab-separated \
            'SELECT Type1 { prop1 }')
        test "$val" = "value1"
    "###,
    )
    .success();
    Ok(())
}
