require "widget"

RSpec.describe Gadget do
  describe "#spin" do
    it "spins" do
      expect(Gadget.new.spin).to eq 1
    end
  end

  describe "templates" do
    it "lists none" do
      expect(Dir["templates/*"]).to be_empty
    end
  end
end
