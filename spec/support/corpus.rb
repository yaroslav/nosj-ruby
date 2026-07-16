# frozen_string_literal: true

# The benchmark corpus doubles as the parity fixture set: every file is
# real-world JSON that both parsers must agree on byte-for-byte.
module CorpusHelper
  CORPUS = Dir[File.expand_path("../../benchmark/*.json", __dir__)].sort.freeze

  def corpus_files
    CORPUS
  end
end

RSpec.configure do |config|
  config.include CorpusHelper
end
