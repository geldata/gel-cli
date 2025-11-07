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
    sequence {
        ! gel warning: DeprecatedScopingError at 1:1
        ! Schema does not have 'using future simple_scoping'.
        ! Non-simple_scoping will be removed in Gel 8.0.
        ! See https://docs.geldata.com/reference/edgeql/path_resolution
        !
    }
    # This should probably not be here, it was happening in project/link.cli:178
    ! ERROR %{GREEDYDATA} edb.server: HttpClient %{GREEDYDATA} was not closed
}
