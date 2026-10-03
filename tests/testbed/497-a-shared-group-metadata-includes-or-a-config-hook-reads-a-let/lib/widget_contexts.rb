RSpec.shared_context "with a widget config" do
  let(:config) { { "Widget" => widget_options } }
end

RSpec.configure do |config|
  config.include_context "with a widget config", :widget
end
