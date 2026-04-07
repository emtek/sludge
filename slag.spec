Name:           slag
Version:        0.2.0
Release:        1%{?dist}
Summary:        A Slack client for the Linux desktop

License:        GPL-3.0-or-later
URL:            https://github.com/emlyn/slag
Source0:        %{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4) >= 4.14
BuildRequires:  pkgconfig(libadwaita-1) >= 1.4
BuildRequires:  pkgconfig(openssl)
BuildRequires:  desktop-file-utils

Requires:       gtk4 >= 4.14
Requires:       libadwaita >= 1.4

%description
Slag is a native GTK4/libadwaita Slack client for Fedora and other
Linux desktops. It provides a fast, low-memory interface for Slack
workspaces with support for channels, threads, emoji, and images.

%prep
%autosetup

%build
cargo build --release

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm644 assets/%{name}.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
install -Dm644 assets/hicolor/256x256/apps/%{name}.png %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/%{name}.png
install -Dm644 assets/hicolor/128x128/apps/%{name}.png %{buildroot}%{_datadir}/icons/hicolor/128x128/apps/%{name}.png
install -Dm644 assets/hicolor/64x64/apps/%{name}.png %{buildroot}%{_datadir}/icons/hicolor/64x64/apps/%{name}.png
install -Dm644 assets/hicolor/48x48/apps/%{name}.png %{buildroot}%{_datadir}/icons/hicolor/48x48/apps/%{name}.png

desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop

%files
%{_bindir}/%{name}
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/*/apps/%{name}.png

%changelog
* Tue Apr 07 2026 Emlyn Revell-Nash <emlyn@localhost> - 0.1.0-1
- Initial RPM package
