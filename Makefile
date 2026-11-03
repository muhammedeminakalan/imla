.PHONY: build test dist package clean

build:
	./build.sh build --release

test:
	./build.sh test --release

dist: test build
	mkdir -p dist
	install -m755 target/release/imla dist/imla

# Arch package in packaging/arch (imla-<ver>-1-x86_64.pkg.tar.zst)
package: dist
	cd packaging/arch && rm -f *.pkg.tar.zst && makepkg -f

clean:
	rm -rf dist packaging/arch/pkg packaging/arch/src packaging/arch/*.pkg.tar.zst
