module Widget
  module AllowedMethods
    def allowed_methods
    end
  end

  # Deprecated: use AllowedMethods.
  IgnoredMethods = AllowedMethods

  LoadMonitor = Deprecation::DeprecatedConstantProxy.new("Widget::LoadMonitor", "Monitor")

  LIMIT = 3
end
