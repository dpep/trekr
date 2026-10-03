RSpec.describe "Alpha" do
  let(:name) { "x" }
  let(:size) { 1 }

  it_behaves_like "a named thing"
  it_behaves_like "a sized thing"
end
