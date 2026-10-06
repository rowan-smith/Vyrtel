use tantivy::schema::{DateOptions, DateTimePrecision, Schema, SchemaBuilder, FAST, STORED, STRING, TEXT};

#[derive(Clone)]
pub struct SchemaFields {
    pub id: tantivy::schema::Field,
    pub timestamp: tantivy::schema::Field,
    pub event_type: tantivy::schema::Field,
    pub level: tantivy::schema::Field,
    pub service: tantivy::schema::Field,
    pub environment: tantivy::schema::Field,
    pub trace_id: tantivy::schema::Field,
    pub span_id: tantivy::schema::Field,
    pub parent_span_id: tantivy::schema::Field,
    pub message: tantivy::schema::Field,
    pub message_template: tantivy::schema::Field,
    pub stacktrace: tantivy::schema::Field,
    pub duration_ns: tantivy::schema::Field,
    pub attributes: tantivy::schema::Field,
}

pub struct EventSchema {
    pub schema: Schema,
    pub fields: SchemaFields,
}

impl EventSchema {
    pub fn build() -> Self {
        let mut builder = SchemaBuilder::new();

        // STRING already implies indexed (untokenized).
        let id = builder.add_text_field("id", STRING | STORED);
        // Microsecond fast-field precision matches what the doc store keeps, so sort order,
        // range filters and pagination cursors all agree (the default is whole seconds).
        let timestamp = builder.add_date_field(
            "timestamp",
            DateOptions::default()
                .set_indexed()
                .set_fast()
                .set_stored()
                .set_precision(DateTimePrecision::Microseconds),
        );
        let event_type = builder.add_text_field("event_type", STRING | FAST | STORED);
        let level = builder.add_text_field("level", STRING | FAST | STORED);
        let service = builder.add_text_field("service", STRING | FAST | STORED);
        let environment = builder.add_text_field("environment", STRING | FAST | STORED);
        let trace_id = builder.add_text_field("trace_id", STRING | FAST | STORED);
        let span_id = builder.add_text_field("span_id", STRING | STORED);
        let parent_span_id = builder.add_text_field("parent_span_id", STRING | STORED);
        let message = builder.add_text_field("message", TEXT | STORED);
        let message_template = builder.add_text_field("message_template", TEXT | STORED);
        let stacktrace = builder.add_text_field("stacktrace", TEXT | STORED);
        let duration_ns =
            builder.add_u64_field("duration_ns", tantivy::schema::INDEXED | FAST | STORED);
        // TEXT enables inverted index on JSON; STORED keeps the object.
        let attributes = builder.add_json_field("attributes", STORED | TEXT);

        let schema = builder.build();
        Self {
            schema,
            fields: SchemaFields {
                id,
                timestamp,
                event_type,
                level,
                service,
                environment,
                trace_id,
                span_id,
                parent_span_id,
                message,
                message_template,
                stacktrace,
                duration_ns,
                attributes,
            },
        }
    }
}
