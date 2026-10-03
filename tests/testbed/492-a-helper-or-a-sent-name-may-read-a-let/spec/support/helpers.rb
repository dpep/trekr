module WidgetHelpers
  def sign_in_widget
    log_in(current_widget)
  end
end

RSpec.configure do |config|
  config.include WidgetHelpers
end
