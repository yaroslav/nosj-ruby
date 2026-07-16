# frozen_string_literal: true

RSpec.describe "sig/nosj.rbs" do
  it "signs only methods that exist at runtime" do
    sig = File.read(File.expand_path("../sig/nosj.rbs", __dir__))
    signed = sig.scan(/def self\.(\w+\??):/).flatten
    expect(signed).not_to be_empty
    signed.each do |name|
      expect(NOSJ).to respond_to(name), "signed but missing at runtime: NOSJ.#{name}"
    end
  end

  it "signs every public singleton method of NOSJ" do
    sig = File.read(File.expand_path("../sig/nosj.rbs", __dir__))
    signed = sig.scan(/def self\.(\w+\??):/).flatten.map(&:to_sym)
    public_api = NOSJ.singleton_methods(false).sort - signed
    # The *_native entry points are implementation detail.
    public_api.reject! { |m| m.to_s.end_with?("_native") || m == :parse_nogvl }
    expect(public_api).to be_empty, "unsigned public methods: #{public_api.join(", ")}"
  end
end
