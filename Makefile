CARGO ?= cargo

.PHONY: build

target_dir = $(or $(CARGO_TARGET_DIR),target)

# Everything the package ships is staged here, so that a packaging change ships with its release once the Homebrew formula installs this package.
build:
	@case "$(OUT)" in /*) ;; *) printf '%s\n' 'Set OUT to an absolute directory to stage the package in.' >&2; exit 1;; esac
	$(CARGO) build --locked --release --target-dir "$(target_dir)"
	mkdir -p "$(OUT)/bin"
	install -m 755 "$(target_dir)/release/sessidx" "$(OUT)/bin/sessidx"
	install -m 644 LICENSE README.md "$(OUT)/"
# cargo metadata needs every target's crates, which no build fetches; cargo-about only logs a dropped notice, such as a clarification whose checksum no longer matches, so any warning or error fails.
	$(CARGO) fetch --locked
	$(CARGO) about generate --frozen --fail --output-file "$(OUT)/LICENSE-THIRD-PARTY.md" about.hbs 2> "$(target_dir)/cargo-about.log" || { cat "$(target_dir)/cargo-about.log" >&2; exit 1; }
	@cat "$(target_dir)/cargo-about.log" >&2; ! grep -Eq 'WARN|ERROR' "$(target_dir)/cargo-about.log"
