RSpec.describe "Alpha" do
  let(:thing_name) { "alpha" }
  it_behaves_like "uses thing name"
  include_examples "always named"
end

RSpec.describe "Beta" do
  it_behaves_like "uses thing name"
end
