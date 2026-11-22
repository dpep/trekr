class Widget
  def self.kinds
    Object.const_get("Ñu")
  end
end
