Name:           sludge
Version:        0.2.3
Release:        1%{?dist}
Summary:        A native GTK4/libadwaita Slack client for the Linux desktop

License:        GPL-3.0-or-later
URL:            https://github.com/emtek/sludge
Source0:        %{name}-%{version}.tar.gz

# Build requirements — matches what Cargo pulls in
BuildRequires:  cargo
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4) >= 4.14
BuildRequires:  pkgconfig(libadwaita-1) >= 1.4
BuildRequires:  pkgconfig(gdk-pixbuf-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(cairo)
BuildRequires:  pkgconfig(pango)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Runtime requirements
Requires:       gtk4 >= 4.14
Requires:       libadwaita >= 1.4
Requires:       hicolor-icon-theme
# Optional but recommended
Recommends:     libcanberra-gtk3  # notification sounds via canberra-gtk-play
Recommends:     libnotify         # notify-send for desktop notifications

# Tested on Fedora 43
ExclusiveArch:  %{rust_arches}

%description
Sludge is a native GTK4/libadwaita Slack client for the Linux desktop.
Features include real-time messaging via Socket Mode, inline threads,
reactions, file uploads, desktop notifications, a GNOME Shell search
provider, and fuzzy emoji autocomplete.

%prep
%autosetup -n %{name}-%{version}

%build
# Use the system-wide target dir so %%install sees the binary under target/release
cargo build --release --locked

%install
rm -rf %{buildroot}

install -Dpm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}

# Desktop file — rewrite Exec to the absolute install path
install -d %{buildroot}%{_datadir}/applications
sed "s|^Exec=.*|Exec=%{_bindir}/%{name}|" \
    assets/dev.sludge.app.desktop \
    > %{buildroot}%{_datadir}/applications/dev.sludge.app.desktop
chmod 644 %{buildroot}%{_datadir}/applications/dev.sludge.app.desktop

# Icons
for size in 48 64 128 256; do
    install -Dpm644 assets/hicolor/${size}x${size}/apps/%{name}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/%{name}.png
done

# GNOME Shell search provider
install -Dpm644 assets/dev.sludge.app.search-provider.ini \
    %{buildroot}%{_datadir}/gnome-shell/search-providers/dev.sludge.app.search-provider.ini

# D-Bus activation service — rewrite Exec path to system binary
install -d %{buildroot}%{_datadir}/dbus-1/services
sed "s|^Exec=.*|Exec=%{_bindir}/%{name}|" \
    assets/dev.sludge.app.service \
    > %{buildroot}%{_datadir}/dbus-1/services/dev.sludge.app.service
chmod 644 %{buildroot}%{_datadir}/dbus-1/services/dev.sludge.app.service

# Validate desktop file
desktop-file-validate %{buildroot}%{_datadir}/applications/dev.sludge.app.desktop

%files
%doc README.md
%{_bindir}/%{name}
%{_datadir}/applications/dev.sludge.app.desktop
%{_datadir}/icons/hicolor/*/apps/%{name}.png
%{_datadir}/gnome-shell/search-providers/dev.sludge.app.search-provider.ini
%{_datadir}/dbus-1/services/dev.sludge.app.service

%post
/usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/update-desktop-database %{_datadir}/applications &>/dev/null || :

%postun
/usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/update-desktop-database %{_datadir}/applications &>/dev/null || :

%changelog
* Mon Apr 13 2026 Emlyn Revell-Nash <noreply@example.com> - 0.2.3-1
- Release 0.2.3: inline threads, reply UX, fuzzy emoji search, search
  result navigation to thread replies, reliable scroll-to-row, and more.
* Tue Apr 07 2026 Emlyn Revell-Nash <noreply@example.com> - 0.2.2-1
- Initial RPM packaging.
