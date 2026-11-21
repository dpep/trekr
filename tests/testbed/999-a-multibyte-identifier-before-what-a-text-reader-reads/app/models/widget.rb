class Widget
  def self.all_kinds
    año.descendants
    año.subclasses
    año.constants.map { |c| año.const_get(c) }
    año.const_get(nombre)
    T.unsafe(año).descendants
    señal.constantize
    año.public_send(señal)
    UsedKit.render(año)
  end

  def tamaño
    :"año_#{señal}"
  end
end
