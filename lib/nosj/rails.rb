# frozen_string_literal: true

# Rails mode: `require "nosj/rails"` accelerates a Rails application in
# both directions.
#
# - It installs {NOSJ::RailsEncoder} as ActiveSupport's JSON encoder
#   (the official +ActiveSupport::JSON::Encoding.json_encoder+ seam, the
#   same one Oj's Rails mode uses). That captures every encode a Rails
#   app performs through +obj.to_json+, +render json:+, and
#   +ActiveSupport::JSON.encode+: the object tree is walked natively,
#   values that are not JSON-native recurse through +as_json+ exactly
#   like ActiveSupport's own encoder, and non-finite floats encode as
#   +null+ (+Float#as_json+ parity).
# - It loads the `nosj/json` drop-in, so +JSON.parse+ (and with it
#   +ActiveSupport::JSON.decode+ and JSON request-body parsing) takes
#   the fast path.
#
# In a Rails Gemfile:
#
#   gem "nosj", require: "nosj/rails"
#
# Known divergence: +JSON::Fragment+ values raise instead of splicing
# raw JSON (fragments are unsupported gem-wide).

require "nosj/json"
require "active_support"
require "active_support/json"

module NOSJ
  # ActiveSupport JSON encoder backed by nosj: the interface of
  # ActiveSupport's +JSONGemEncoder+ (accepts the options hash, encodes
  # one value), with the tree walk, generation, AND the HTML/JS-safety
  # escape pass running natively (a byte scan instead of ActiveSupport's
  # Ruby regex post-pass), honoring +escape_html_entities_in_json+ (and,
  # where present, +escape_js_separators_in_json+ and the per-call
  # +escape:+ / +escape_html_entities:+ options).
  class RailsEncoder
    # The escape_js_separators_in_json knob is newer than the Rails
    # versions we support; its presence is fixed at load time (only its
    # value can change at runtime). Absent, ActiveSupport always
    # escapes the JS separators.
    HAS_JS_SEPARATORS_KNOB =
      ActiveSupport::JSON::Encoding.respond_to?(:escape_js_separators_in_json)
    private_constant :HAS_JS_SEPARATORS_KNOB

    attr_reader :options

    def initialize(options = nil)
      @options = options || {}
    end

    def encode(value)
      value = value.as_json(@options.dup) unless @options.empty?
      if @options.fetch(:escape, true)
        escape_html = @options.fetch(:escape_html_entities) do
          ActiveSupport::JSON::Encoding.escape_html_entities_in_json
        end
        NOSJ.generate_rails_native(value, !!escape_html, escape_js_separators?)
      else
        NOSJ.generate_rails_native(value, false, false)
      end
    end

    private

    def escape_js_separators?
      !HAS_JS_SEPARATORS_KNOB ||
        ActiveSupport::JSON::Encoding.escape_js_separators_in_json
    end
  end

  ActiveSupport::JSON::Encoding.json_encoder = RailsEncoder
end
