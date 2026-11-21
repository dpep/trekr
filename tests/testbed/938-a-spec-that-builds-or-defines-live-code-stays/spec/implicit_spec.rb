RSpec.describe Counter do
  it "starts" do
    expect(Implicit.new.implicit_value).to eq 2
    expect { subject }.not_to raise_error
  end
end
