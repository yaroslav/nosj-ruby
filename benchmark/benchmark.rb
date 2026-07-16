# frozen_string_literal: true

# Multi-gem JSON shoot-out: every installed parser AND generator, on
# benchmark-ips, across the benchmark corpus.
#
# Usage:
#   bundle exec rake bench:ips                     # all files
#   bundle exec rake "bench:ips[twitter,canada]"
#
# This is the ecosystem comparison. For the trusted nosj-vs-gem ratios
# (alternating harness, parity gate) use `rake bench`.

require "benchmark/ips"
require "json"
require_relative "../lib/nosj"

RubyVM::YJIT.enable

# Each contender is optional: a gem that fails to load (or breaks on this
# Ruby) is reported and skipped, not fatal.
PARSERS = {}
GENERATORS = {}
VERSIONS = {}

PARSERS["JSON"] = ->(s) { JSON.parse(s) }
GENERATORS["JSON"] = ->(o) { JSON.generate(o) }
VERSIONS["JSON"] = JSON::VERSION

begin
  require "oj"
  # :compat mode produces plain Hash/String values like JSON.parse.
  PARSERS["Oj"] = ->(s) { Oj.load(s, mode: :compat) }
  GENERATORS["Oj"] = ->(o) { Oj.dump(o, mode: :compat) }
  VERSIONS["Oj"] = Oj::VERSION
rescue LoadError, StandardError => e
  warn "Oj unavailable: #{e.class}"
end

begin
  require "rapidjson"
  PARSERS["RapidJSON"] = ->(s) { RapidJSON.parse(s) }
  GENERATORS["RapidJSON"] = ->(o) { RapidJSON.dump(o) }
  VERSIONS["RapidJSON"] = RapidJSON::VERSION
rescue LoadError, StandardError => e
  warn "RapidJSON unavailable: #{e.class}"
end

begin
  require "fast_jsonparser"
  PARSERS["FastJsonparser"] = ->(s) { FastJsonparser.parse(s, symbolize_keys: false) }
  # fast_jsonparser has no generator.
  VERSIONS["FastJsonparser"] = Gem.loaded_specs["fast_jsonparser"]&.version.to_s
rescue LoadError, StandardError => e
  warn "FastJsonparser unavailable: #{e.class}"
end

begin
  require "yajl"
  PARSERS["Yajl"] = ->(s) { Yajl::Parser.parse(s) }
  GENERATORS["Yajl"] = ->(o) { Yajl::Encoder.encode(o) }
  VERSIONS["Yajl"] = Gem.loaded_specs["yajl-ruby"]&.version.to_s
rescue LoadError, StandardError => e
  warn "Yajl unavailable: #{e.class}"
end

PARSERS["NOSJ"] = ->(s) { NOSJ.parse(s) }
GENERATORS["NOSJ"] = ->(o) { NOSJ.generate(o) }
VERSIONS["NOSJ"] = "dev"

puts "ruby #{RUBY_VERSION} (YJIT #{RubyVM::YJIT.enabled?}) — " +
  VERSIONS.map { |n, v| "#{n} #{v}" }.join(", ")

all_files = Dir[File.join(__dir__, "*.json")].sort
files = if ARGV.empty?
  all_files
else
  ARGV.map do |name|
    all_files.find { |f| File.basename(f, ".json") == name } ||
      abort("unknown benchmark file: #{name} (have: #{all_files.map { |f| File.basename(f, ".json") }.join(", ")})")
  end
end

files.each do |filename|
  data = File.read(filename)
  reference = JSON.parse(data)
  puts "\n\n## #{File.basename(filename)} (#{data.size} bytes)"

  # Sanity gate: a contender that returns different values (or emits JSON
  # that doesn't parse back to the same values) is benchmarked anyway, but
  # flagged: speed comparisons only mean something at equal output.
  PARSERS.each do |name, parse|
    result = begin
      parse.call(data)
    rescue => e
      warn "  #{name} parse FAILED on this file: #{e.class} — skipping"
      PARSERS.delete(name) # rubocop:disable Lint/UnreachableLoop -- intentional narrowing
      next
    end
    warn "  NOTE: #{name} parse output differs from JSON.parse" unless result == reference
  end
  GENERATORS.each do |name, generate|
    round_trip = begin
      JSON.parse(generate.call(reference))
    rescue => e
      warn "  #{name} generate FAILED on this file: #{e.class}"
      nil
    end
    warn "  NOTE: #{name} generate does not round-trip to equal values" if round_trip && round_trip != reference
  end

  puts "\n### parse"
  Benchmark.ips do |x|
    x.warmup = 1
    x.time = 3
    PARSERS.each do |name, parse|
      x.report(name) { parse.call(data) }
    end
    x.compare!
  end

  puts "\n### generate"
  Benchmark.ips do |x|
    x.warmup = 1
    x.time = 3
    GENERATORS.each do |name, generate|
      x.report(name) { generate.call(reference) }
    end
    x.compare!
  end
end
