Name:           dgcu-net-portal
Version:        @VERSION@
Release:        1
Summary:        DGCU 校园网 Portal 客户端（GUI + CLI）
License:        GPL-3.0-only
URL:            https://github.com/miaoermua/DGCU-Net-Portal

# 各发行版的包名不一样：Fedora 是 webkit2gtk4.1 / libayatana-appindicator-gtk3，
# openSUSE 是带下划线和 -1-0 后缀的那套。用 rich dependency 让两边都能解出来。
Requires:       gtk3
Requires:       (webkit2gtk4.1 or libwebkit2gtk-4_1-0)
Requires:       (libayatana-appindicator-gtk3 or libayatana-appindicator3-1)
Requires:       (libsecret or libsecret-1-0)

# 在 Ubuntu 上交叉打包时，自动依赖扫描会把 runner 上并不存在的 soname 写进
# Requires，反而让目标机器装不上；依赖上面已手写，这里关掉自动推导。
AutoReqProv:    no

%description
自动完成东莞城市学院 lfradius portal 的校园网登录，支持掉线重拨与后台常驻认证。

包含图形界面 portal-gui 与命令行/守护进程 portal-cli，两者需装在同一目录下
（GUI 沿自身路径查找同包的 portal-cli）。

%prep

%build

%install
install -Dm755 %{stagedir}/usr/bin/portal-gui %{buildroot}/usr/bin/portal-gui
install -Dm755 %{stagedir}/usr/bin/portal-cli %{buildroot}/usr/bin/portal-cli
install -Dm644 %{stagedir}/usr/share/applications/dgcu-portal.desktop %{buildroot}/usr/share/applications/dgcu-portal.desktop
install -Dm644 %{stagedir}/usr/share/icons/hicolor/256x256/apps/portal-gui.png %{buildroot}/usr/share/icons/hicolor/256x256/apps/portal-gui.png

%files
/usr/bin/portal-gui
/usr/bin/portal-cli
/usr/share/applications/dgcu-portal.desktop
/usr/share/icons/hicolor/256x256/apps/portal-gui.png
%license /usr/share/licenses/dgcu-net-portal/LICENSE
