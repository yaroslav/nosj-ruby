# frozen_string_literal: true

RSpec.describe NOSJ do
  it "has a version number" do
    expect(NOSJ::VERSION).not_to be_nil
  end

  it "exposes the error classes" do
    expect(NOSJ::GeneratorError).to be < NOSJ::Error
    expect(NOSJ::NestingError).to be < NOSJ::Error
    expect(NOSJ::Error).to be < StandardError
  end
end
