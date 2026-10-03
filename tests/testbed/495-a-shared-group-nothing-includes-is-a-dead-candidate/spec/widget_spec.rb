RSpec.describe "Widget" do
  subject { 1 }

  it_behaves_like "a used thing"

  shared_examples "a local thing" do
    it { expect(true).to be(true) }
  end

  shared_examples "a local unused thing" do
    it { expect(true).to be(true) }
  end

  include_examples "a local thing"
end
