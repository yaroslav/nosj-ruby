# frozen_string_literal: true

require_relative "lib/nosj/version"

Gem::Specification.new do |spec|
  spec.name = "nosj"
  spec.version = NOSJ::VERSION
  spec.authors = ["Yaroslav Markin"]
  spec.email = ["yaroslav@markin.net"]

  spec.summary = "An extremely fast JSON parser and " \
    "generator for Ruby, written in Rust and SIMD-accelerated on every platform."
  spec.description = "gem nosj is an extremely fast, json-gem-compatible JSON parser and generator " \
    "for Ruby: Rust and SIMD via the first-party nosj crate, precompiled platform gems " \
    "with per-platform PGO, partial parsing (JSON Pointer, single and batch), " \
    "lazy documents that parse a value only when you touch it, file APIs that " \
    "parse, generate, and query files natively (memory-mapped, so unread pages " \
    "never leave the disk), allocation-free validation, a one-line JSON " \
    "module drop-in, and a Rails mode that plugs into ActiveSupport's " \
    "encoder seam."
  spec.homepage = "https://github.com/yaroslav/nosj-ruby"
  spec.license = "MIT"
  spec.required_ruby_version = ">= 3.3.0"
  spec.required_rubygems_version = ">= 3.3.11"

  spec.metadata["allowed_push_host"] = "https://rubygems.org"
  spec.metadata["homepage_uri"] = spec.homepage
  spec.metadata["source_code_uri"] = spec.homepage
  spec.metadata["changelog_uri"] = "#{spec.homepage}/blob/main/CHANGELOG.md"
  spec.metadata["bug_tracker_uri"] = "#{spec.homepage}/issues"
  spec.metadata["documentation_uri"] = "https://rubydoc.info/gems/nosj"
  spec.metadata["rubygems_mfa_required"] = "true"

  # Sources only: built .bundle/.so artifacts must never ride into the
  # source gem (the platform-gem task builds its own file list).
  spec.files = Dir["lib/**/*.rb"] +
    Dir["sig/**/*.rbs"] +
    Dir["ext/**/*.{rb,rs,toml}"] +
    %w[README.md CHANGELOG.md LICENSE.txt NOTICE Cargo.toml Cargo.lock]
  spec.require_paths = ["lib"]
  spec.extensions = ["ext/nosj/extconf.rb"]

  # Build-time only; the gem:native task strips it from platform gems.
  spec.add_dependency "rb_sys", "~> 0.9.91"
end
