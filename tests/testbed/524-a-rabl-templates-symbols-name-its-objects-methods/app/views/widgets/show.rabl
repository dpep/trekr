object @widget
attributes :label, :label => :title
child(:parts) do
  attributes :sku
end
node(:x) { |w| w.label }
attributes(*widget_fields)
extends "widgets/base"
