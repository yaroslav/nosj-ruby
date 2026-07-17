# frozen_string_literal: true

# Installing the encoder mutates ActiveSupport globally (and loading
# activesupport pollutes core classes), so every assertion runs in a
# subprocess, differentially against ActiveSupport's own encoder.
RSpec.describe "require 'nosj/rails'" do
  def expect_ok(script)
    out = IO.popen(
      [RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), "-e", script],
      err: [:child, :out], &:read
    )
    expect($?.success?).to be(true), out
    expect(out).to include("ALL-OK")
  end

  it "matches ActiveSupport's encoder byte-for-byte across the battery" do
    expect_ok(<<~'RUBY')
      require "active_support"
      require "active_support/json"
      require "bigdecimal"

      custom = Class.new { def as_json(_ = nil) = {"custom" => true} }.new
      # Real-world escape-heavy content (tweets are full of & and <)
      # pins the native escape pass; the subprocess inherits the repo
      # root as cwd.
      corpus = %w[benchmark/twitter.json benchmark/activitypub.json]
        .select { |f| File.exist?(f) }.map { |f| JSON.parse(File.read(f)) }
      fixtures = corpus + [
        {"a" => [1, true, nil], "sym" => :sym, "f" => 2.5},
        {"html" => "<script>alert('&')</script>"},
        ["line" + 0x2028.chr(Encoding::UTF_8) + "sep" + 0x2029.chr(Encoding::UTF_8)],
        [Float::NAN, Float::INFINITY, -Float::INFINITY, 1.5],
        Time.at(0).utc, Date.new(2026, 7, 16), BigDecimal("1.5"),
        {1 => "int key", nil => "nil key"},
        custom, [custom],
        "plain", 42, nil, true,
        {"deep" => {"er" => {"est" => [[[1]]]}}}
      ]

      stock = fixtures.map { |v| ActiveSupport::JSON.encode(v) }
      stock_to_json = fixtures.map(&:to_json)
      stock_opts = ActiveSupport::JSON.encode({"a" => 1, "b" => 2}, only: "a")

      require "nosj/rails"

      fixtures.each_with_index do |v, i|
        mine = ActiveSupport::JSON.encode(v)
        raise "encode mismatch at #{i}: #{stock[i].inspect} vs #{mine.inspect}" unless stock[i] == mine
        raise "to_json mismatch at #{i}" unless v.to_json == stock_to_json[i]
      end
      unless ActiveSupport::JSON.encode({"a" => 1, "b" => 2}, only: "a") == stock_opts
        raise "options not forwarded to as_json"
      end
      puts "ALL-OK"
    RUBY
  end

  it "matches stock for SafeBuffer strings and time_precision config" do
    expect_ok(<<~'RUBY')
      require "active_support"
      require "active_support/json"
      require "active_support/core_ext/string/output_safety"

      fixtures = [
        {"safe" => "<b>bold</b>".html_safe, "plain" => "<b>bold</b>"},
        {"t" => Time.at(0, 123456, :usec).utc}
      ]
      ActiveSupport::JSON::Encoding.time_precision = 6
      stock = fixtures.map { |v| ActiveSupport::JSON.encode(v) }

      require "nosj/rails"

      fixtures.each_with_index do |v, i|
        mine = ActiveSupport::JSON.encode(v)
        raise "mismatch #{i}: #{stock[i].inspect} vs #{mine.inspect}" unless mine == stock[i]
      end
      # The config must actually be honored (6 fractional digits), not
      # just match stock.
      raise "precision" unless ActiveSupport::JSON.encode(fixtures[1]) =~ /\.\d{6}/
      puts "ALL-OK"
    RUBY
  end

  it "honors the escape_html_entities_in_json config both ways" do
    expect_ok(<<~RUBY)
      require "active_support"
      require "active_support/json"

      probe = {"h" => "<&>"}
      ActiveSupport::JSON::Encoding.escape_html_entities_in_json = false
      stock_off = ActiveSupport::JSON.encode(probe)
      ActiveSupport::JSON::Encoding.escape_html_entities_in_json = true
      stock_on = ActiveSupport::JSON.encode(probe)

      require "nosj/rails"

      raise "escaped mismatch" unless ActiveSupport::JSON.encode(probe) == stock_on
      ActiveSupport::JSON::Encoding.escape_html_entities_in_json = false
      raise "unescaped mismatch" unless ActiveSupport::JSON.encode(probe) == stock_off
      raise "not actually unescaped" unless ActiveSupport::JSON.encode(probe).include?("<&>")
      puts "ALL-OK"
    RUBY
  end

  it "splices JSON::Fragment like ActiveSupport does" do
    expect_ok(<<~'RUBY')
      require "active_support"
      require "active_support/json"
      require "json"

      value = {"cached" => JSON::Fragment.new('{"pre":"rendered"}')}
      stock = ActiveSupport::JSON.encode(value)

      require "nosj/rails"

      mine = ActiveSupport::JSON.encode(value)
      raise "fragment mismatch: #{stock.inspect} vs #{mine.inspect}" unless stock == mine
      raise "not spliced" unless mine == '{"cached":{"pre":"rendered"}}'
      puts "ALL-OK"
    RUBY
  end

  it "routes ActiveSupport::JSON.decode through the drop-in fast path" do
    expect_ok(<<~RUBY)
      require "active_support"
      require "active_support/json"
      require "nosj/rails"

      parsed = ActiveSupport::JSON.decode('{"a":[1,true],"n":1.5}')
      raise "decode" unless parsed == {"a" => [1, true], "n" => 1.5}
      # Rails 7.x passes quirks_mode: true; the fast path must accept it.
      raise "quirks" unless JSON.parse("2", quirks_mode: true) == 2
      puts "ALL-OK"
    RUBY
  end

  it "raises instead of looping when as_json returns the receiver" do
    expect_ok(<<~RUBY)
      require "active_support"
      require "active_support/json"
      require "nosj/rails"

      selfish = Class.new { def as_json(_ = nil) = self }.new
      begin
        ActiveSupport::JSON.encode(selfish)
        raise "no error raised"
      rescue NOSJ::GeneratorError => e
        raise "wrong message" unless e.message.include?("as_json returned the receiver")
      end
      puts "ALL-OK"
    RUBY
  end
end
