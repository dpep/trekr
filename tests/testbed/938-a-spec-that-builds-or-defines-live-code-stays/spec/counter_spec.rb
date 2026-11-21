RSpec.describe Counter do
  it "bumps" do
    c = Counter.new
    expect { c.bump! }.to change(c, :count).by(10)
  end
end
