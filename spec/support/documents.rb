# frozen_string_literal: true

# Generated documents for the duplicate-key specs.
module DocumentHelper
  # An object with keys "k1".."k<size>" (values 1..size), optionally
  # repeating the key `repeat` at the end. Object sizes pick the
  # close-time check: pairwise up to 8 keys, a table up to 128, a sort
  # beyond.
  def keyed_object(size, repeat: nil)
    pairs = (1..size).map { %("k#{_1}": #{_1}) }
    pairs << %("#{repeat}": 0) if repeat
    "{#{pairs.join(",")}}"
  end
end

RSpec.configure do |config|
  config.include DocumentHelper
end
