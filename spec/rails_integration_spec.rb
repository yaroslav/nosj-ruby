# frozen_string_literal: true

# Full-stack Rails integration: a real ActionDispatch route set and
# ActionController::API controller, driven through the Rack interface.
# JSON request bodies enter through ActionDispatch's parameter parsing
# (ActiveSupport::JSON.decode -> the nosj/json drop-in) and responses
# leave through render json: (Object#to_json -> the nosj Rails
# encoder). Every example runs the same requests before and after
# `require "nosj/rails"` in one subprocess and compares the raw
# response bytes.
RSpec.describe "nosj/rails ActionDispatch integration" do
  def expect_ok(script)
    out = IO.popen(
      [RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), "-e", script],
      err: [:child, :out], &:read
    )
    expect($?.success?).to be(true), out
    expect(out).to include("ALL-OK")
  end

  def app_prelude
    <<~RUBY
        require "action_controller"

      class ApiController < ActionController::API
        def echo
          render json: {"params" => request.request_parameters, "q" => params[:q]}
        end

        def show
          render json: {
            "time" => Time.at(0).utc,
            "floats" => [2.5, Float::NAN, Float::INFINITY],
            "html" => "<script>alert('&')</script>",
            "sym" => :ok,
            "deep" => {"arr" => [1, [2, [3, {"k" => nil}]]]},
            "model" => Class.new { def as_json(_ = nil) = {"custom" => true} }.new
          }
        end
      end

      ROUTES = ActionDispatch::Routing::RouteSet.new
      ROUTES.draw do
        post "/echo", to: "api#echo"
        get "/show", to: "api#show"
      end

      def hit(method, path, body = nil)
        env = Rack::MockRequest.env_for(
          path,
          method: method,
          input: body,
          "CONTENT_TYPE" => (body ? "application/json" : nil)
        )
        status, _headers, response = ROUTES.call(env)
        chunks = +""
        response.each { |c| chunks << c }
        [status, chunks]
      rescue => e
        [:raised, e.class.name]
      end
    RUBY
  end

  it "serves byte-identical responses through the full request cycle" do
    expect_ok(app_prelude + <<~'RUBY')
      requests = [
        [:get, "/show", nil],
        [:post, "/echo?q=1", '{"user":{"name":"ada","tags":["x","y"]},"n":12345678901234567890,"f":1.5}'],
        [:post, "/echo", '{"unicode":"проверка   done","html":"<&>"}'],
        [:post, "/echo", '{"deep":' + ("[" * 50) + "1" + ("]" * 50) + "}"]
      ]
      stock = requests.map { |r| hit(*r) }
      stock.each { |status, _| raise "stock request failed: #{status}" unless status == 200 }

      require "nosj/rails"

      requests.each_with_index do |r, i|
        mine = hit(*r)
        unless mine == stock[i]
          raise "MISMATCH on #{r[1]}:\n  stock: #{stock[i].inspect}\n  nosj:  #{mine.inspect}"
        end
      end
      puts "ALL-OK"
    RUBY
  end

  it "actually routes through nosj in both directions" do
    expect_ok(app_prelude + <<~'RUBY')
      require "nosj/rails"

      raise "encoder not installed" unless
        ActiveSupport::JSON::Encoding.json_encoder == NOSJ::RailsEncoder
      raise "drop-in not installed" unless JSON.respond_to?(:nosj_original_parse, true)

      status, body = hit(:post, "/echo", '{"user":"ada"}')
      raise "status #{status}" unless status == 200
      raise "params did not round-trip: #{body}" unless
        body.include?('"params":{"user":"ada"}')

      status, body = hit(:get, "/show")
      raise "status #{status}" unless status == 200
      raise "non-finite floats" unless body.include?("[2.5,null,null]")
      raise "html escaping" unless body.include?('\\u003cscript\\u003e')
      raise "as_json model" unless body.include?('"model":{"custom":true}')
      puts "ALL-OK"
    RUBY
  end

  it "propagates malformed request bodies exactly like stock" do
    expect_ok(app_prelude + <<~'RUBY')
      bad = '{"broken":'
      stock = hit(:post, "/echo", bad)
      raise "stock did not raise: #{stock.inspect}" unless stock[0] == :raised

      require "nosj/rails"

      mine = hit(:post, "/echo", bad)
      unless mine == stock
        raise "error mismatch: stock #{stock.inspect} vs nosj #{mine.inspect}"
      end
      puts "ALL-OK"
    RUBY
  end
end
