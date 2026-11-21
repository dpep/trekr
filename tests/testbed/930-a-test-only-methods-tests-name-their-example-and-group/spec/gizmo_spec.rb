require "widget"

RSpec.describe Gizmo do
  it "whirls" do
    expect(Gizmo.new.whirl).to eq 2
  end
end
