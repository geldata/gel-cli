ARG codename=jammy
FROM ubuntu:${codename}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y \
    ca-certificates sudo gnupg2 apt-transport-https curl \
    software-properties-common dbus-user-session systemd
RUN curl -fsSL https://download.docker.com/linux/ubuntu/gpg | apt-key add -
RUN add-apt-repository \
   "deb [arch=amd64] https://download.docker.com/linux/ubuntu \
   $(lsb_release -cs) \
   stable"
RUN apt-get update && apt-get install -y docker-ce-cli
RUN adduser --home /home/user1 \
    --shell /bin/bash --ingroup users --gecos "Test User" \
    user1
USER user1
WORKDIR /home/user1
ADD ./context/sudoers /etc/sudoers
