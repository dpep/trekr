class Restrung
  def build(name) = send("public_send", name)
  def small_attrs = 1
end
