#!/usr/bin/env clitest --v0

pattern VERSION \d+\.\d+(:?\.\d+)?(?:-(?:alpha|beta|rc|dev)(:?\.\d+)?)?(?:\+[a-f0-9]{7}|local)?;

ignore {
    ? Newer version of gel tool exists %{VERSION} \(current %{VERSION}\). To upgrade run `gel cli upgrade`
    ! WARNING %{GREEDYDATA} postgres: %{GREEDYDATA}
    ! CRITICAL %{GREEDYDATA} postgres: %{GREEDYDATA}
    ! Connecting to Gel instance %{GREEDYDATA}
    ! Version %{VERSION} is already downloaded
    sequence {
        ! Downloading package...
        ! Successfully installed %{VERSION}
    }
}
