RSpec.describe "Widget" do
  if ENV["FAST"]
    let(:speed) { :fast }
  else
    let(:speed) { :slow }
  end

  it { expect(speed).to be }

  shared_examples "a sized thing" do
    it { expect(size).to be }
  end

  shared_examples "a wrapped thing" do
    it_behaves_like "a sized thing" do
      let(:label) { "x" }
    end
  end

  context "when wrapped" do
    let(:size) { 1 }

    it_behaves_like "a wrapped thing"
  end

  shared_context "with a base" do
    let(:base) { 2 }
  end

  context "when based" do
    include_context "with a base"

    context "deeper" do
      it { expect(base).to eq(2) }
    end
  end

  let(:app) { :rack_app }
end
