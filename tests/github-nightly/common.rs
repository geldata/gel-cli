#[derive(Debug, Clone)]
pub enum Distro {
    Ubuntu(&'static str),
    Debian(&'static str),
}

impl Distro {
    pub fn tag_name(&self) -> String {
        match self {
            Distro::Ubuntu(cn) => format!("test-ubuntu-{cn}"),
            Distro::Debian(cn) => format!("test-debian-{cn}"),
        }
    }

    pub fn dockerfile(&self) -> String {
        match self {
            Distro::Ubuntu(codename) => dock_ubuntu(codename),
            Distro::Debian(codename) => dock_debian(codename),
        }
    }
}

pub fn dock_ubuntu(codename: &str) -> String {
    format!(
        r###"
        FROM ubuntu:{codename}
        ENV DEBIAN_FRONTEND=noninteractive
        RUN apt-get update && apt-get install -y \
            ca-certificates sudo gnupg2 apt-transport-https curl \
            software-properties-common dbus-user-session
        RUN curl -fsSL https://download.docker.com/linux/ubuntu/gpg | apt-key add -
        RUN add-apt-repository \
           "deb [arch=amd64] https://download.docker.com/linux/ubuntu \
           $(lsb_release -cs) \
           stable"
        RUN apt-get update && apt-get install -y docker-ce-cli
        RUN adduser --uid 1000 --home /home/user1 \
            --shell /bin/bash --ingroup users --gecos "Test User" \
            user1
        ADD ./gel /usr/bin/edgedb
        ADD ./gel /usr/bin/gel
        ADD ./sudoers /etc/sudoers
    "###
    )
}

pub fn dock_debian(codename: &str) -> String {
    format!(
        r###"
        FROM debian:{codename}
        ENV DEBIAN_FRONTEND=noninteractive
        RUN apt-get update && apt-get install -y ca-certificates curl sudo
        RUN install -m 0755 -d /etc/apt/keyrings
        RUN curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
        RUN chmod a+r /etc/apt/keyrings/docker.asc

        RUN echo \
            "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian \
            $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
            > /etc/apt/sources.list.d/docker.list
        RUN apt-get update && apt-get install -y docker-ce-cli
        RUN adduser --uid 1000 --home /home/user1 \
            --shell /bin/bash --ingroup users --gecos "Test User" \
            user1
        ADD ./gel /usr/bin/edgedb
        ADD ./gel /usr/bin/gel
        ADD ./sudoers /etc/sudoers
    "###
    )
}
