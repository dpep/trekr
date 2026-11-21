require "labeled"

RSpec.describe "labels" do
  it "maps labels" do
    expect([Labeled.new].map(&:label)).to eq ["l"]
  end
end
