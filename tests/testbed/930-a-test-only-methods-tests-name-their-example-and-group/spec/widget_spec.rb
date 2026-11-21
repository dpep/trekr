require "widget"

RSpec.describe Widget do
  let(:widget) { Widget.new(3) }
  let(:spare) { Widget.new(4) }

  describe "#total" do
    it "doubles" do
      expect(widget.total).to eq 6
    end

    it "doubles the spare" do
      expect(spare.total).to eq 8
    end
  end

  describe "#label" do
    it "names" do
      expect(widget.label).to eq "w3"
    end

    it "sizes" do
      expect(widget.size).to eq 3
    end
  end

  describe "#mixed" do
    it "adds" do
      expect(widget.mixed).to eq(widget.size + 1)
    end
  end

  describe "#peek" do
    it "peeks" do
      thing = make_thing
      expect(widget.peek).to eq(thing.size)
    end
  end

  describe "#tally" do
    before { widget.size }

    it "tallies" do
      expect(widget.tally).to eq 3
    end
  end

  describe "#shade" do
    subject { widget.shade }

    it { is_expected.to eq 3 }
  end

  describe ".build" do
    it "builds" do
      expect(Widget.build).to be_a(Widget)
    end

    it "builds one to total" do
      expect(Widget.build.total).to eq 2
    end
  end
end
