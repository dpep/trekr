RSpec.shared_context "raw http server" do
  let(:port) { 1 }

  def http_response
  end

  before { port }
end

shared_examples "a widget" do
  it { size }
end
