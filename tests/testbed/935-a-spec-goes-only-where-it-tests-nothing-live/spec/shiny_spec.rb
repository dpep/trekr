require "shiny"

RSpec.describe Shiny do
  subject { Shiny.new }

  it "labels" do
    expect(Shiny.new.label).to eq "l"
  end

  it "is visible" do
    expect(Shiny.new.label).to eq "l"
    expect(Shiny.new).to be_visible
  end

  it "has gloss" do
    expect(Shiny.new.label).to eq "l"
    expect(Shiny.new).to have_gloss
  end

  it "has attributes" do
    expect(Shiny.new.label).to eq "l"
    expect(Shiny.new).to have_attributes(name: "w")
  end

  it "maps kinds" do
    expect([Shiny.new].map(&:label)).to eq ["l"]
    expect([Shiny.new].map(&:kind)).to eq [:k]
  end

  its(:name) { expect(Shiny.new.label).to eq "l" }
end
