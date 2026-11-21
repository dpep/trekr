require "shiny"

RSpec.describe Equal do
  it "labels" do
    expect(Equal.new.label).to eq "l"
  end
end
