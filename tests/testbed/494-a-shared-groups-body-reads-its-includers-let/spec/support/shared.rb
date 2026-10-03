RSpec.shared_examples "a named thing" do
  it { expect(name).to eq("x") }
end

RSpec.shared_examples "a sized thing" do
  it { expect(size).to eq(1) }
end
