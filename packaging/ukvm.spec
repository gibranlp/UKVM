Name: ukvm
Version: %{ukvm_version}
Release: 1
Summary: Software KVM and authorized interactive remote sessions for Linux Wayland
License: MIT AND BSD-3-Clause AND IJG AND Zlib
URL: https://github.com/gibranlp/UKVM
Requires: libwayland-client.so.0()(64bit)
Requires: libxkbcommon.so.0()(64bit)
Requires: xdg-desktop-portal
Recommends: systemd
Recommends: pipewire

# The staged ELF is already stripped. Keep automatically generated ABI Requires.
%global debug_package %{nil}
%global __os_install_post %{nil}

%description
Each node can capture or receive explicitly authorized input over pinned TLS.
Requires a compatible desktop portal backend and local user consent.
Installation never enables or starts the user service.

%install
mkdir -p %{buildroot}
cp -a %{ukvm_stage}/usr %{buildroot}/

%files
/usr/bin/ukvm
/usr/lib/systemd/user/ukvm.service
%doc /usr/share/doc/ukvm
