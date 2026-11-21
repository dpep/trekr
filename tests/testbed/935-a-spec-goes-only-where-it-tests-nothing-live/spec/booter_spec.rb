require "booter"

RSpec.describe Booter do
  describe "#first" do
    before_all { Registry.boot! }

    it "is first" do
      expect(Booter.new.first).to eq 1
    end
  end

  describe "#second" do
    let_it_be(:registry) { Registry.boot! }

    it "is second" do
      expect(Booter.new.second).to eq 2
    end
  end

  describe "#third" do
    fab!(:registry) { Registry.boot! }

    it "is third" do
      expect(Booter.new.third).to eq 3
    end
  end
end
