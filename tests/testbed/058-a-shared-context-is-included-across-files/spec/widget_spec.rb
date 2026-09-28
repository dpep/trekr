RSpec.describe Widget do
  include_context "raw http server"

  it { http_response }

  context "when nested" do
    let(:port) { 2 }

    it { port }
  end

  it { port }

  it_behaves_like "a widget" do
    let(:size) { 3 }
  end
end
