class Registry
  def self.find
    Object.const_get(:HandlerA)
    const_defined?("Handlers::HandlerB")
  end
end
Registry.find
