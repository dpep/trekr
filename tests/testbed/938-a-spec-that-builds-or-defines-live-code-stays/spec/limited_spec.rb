RSpec.describe Limited do
  LIMIT = 2

  it "is limited" do
    expect(Limited.new.limited_value).to eq 2
  end
end
