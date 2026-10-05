RSpec.describe "Widget" do
  let(:schema) do
    this = self
    Class.new do
      attribute :part, this.part_class
    end
  end
  let(:part_class) { Class.new }
  let(:query_kind) { :query }
  let(:unused_kind) { :none }

  it "builds" do
    spec = self
    built = Class.new { define_method(:kind) { spec.query_kind } }
    expect(built).to be
    expect(schema).to be
  end

  it "keeps a reassigned alias untyped" do
    other = self
    other = Object.new
    expect(other.unused_kind).to be
  end
end
