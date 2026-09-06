# Maintainer: ThiagoAVicente <todo@example.com>
# Custom fork: adds a "+ Add Desktop" button/keybind and an
# on_current_monitor workspace-switch fix. See:
# https://github.com/Yodayoo/hyprexpose
pkgname=hyprexpose-custom-git
pkgver=r0
pkgrel=1
pkgdesc='Lightweight workspace overview for Hyprland with live window thumbnails (custom fork with an Add Desktop button)'
arch=('x86_64')
url='https://github.com/Yodayoo/hyprexpose'
license=('MIT')
depends=('wayland' 'cairo' 'pango' 'hyprland>=0.55')
makedepends=('git' 'rust' 'cargo')
provides=('hyprexpose' 'hyprexpose-git')
conflicts=('hyprexpose' 'hyprexpose-git')
replaces=('hyprexpose-git')
source=("git+${url}.git#branch=master")
sha256sums=('SKIP')

pkgver() {
    cd hyprexpose
    printf "r%s.%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

build() {
    cd hyprexpose
    cargo build --release --locked
}

package() {
    cd hyprexpose
    install -Dm755 target/release/hyprexpose "$pkgdir/usr/bin/hyprexpose"
    install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
