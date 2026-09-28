RSpec.describe Widget do
  it do
    expect(1).to eq(1)
    expect { 1 }.to raise_error
    is_expected.to eq(2)
    double
  end
end
