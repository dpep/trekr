ActiveSupport.on_load(:widget_test_case) do
  def before_setup
    @routes = :routes
    super
  end
end

module Orphan
end
