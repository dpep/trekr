RSpec.describe Registry do
  it "rejects a blank name" do
    expect(Described.new.described_value).to eq 2
    expect { described_class.new("") }.to raise_error(ArgumentError)
  end
end
